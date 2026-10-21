## Why

A RenderVar with `sourceType = "primvar"` is refused with a warning, so crust
cannot output an arbitrary primvar as an AOV (issue #337). The pass production
asks for most is the **rest position** (`Pref`, an authored `point3f`
`primvars:rest`), which compositing uses for masks that stay on a deforming
surface. Other common needs are UV sets other than `st`, and colour or ID
primvars. Of the passes a production compositing script needs, this one and
motion vectors were the two crust could not produce. Motion vectors have
shipped since.

`add-identity-aovs-openexrid` planned primvar sources as its unblocked first
part (its D4 and its "Primvar sources" requirement). That change is otherwise
on hold until a deep EXR writer exists. This change splits primvar sources
out so they can ship alone. It also corrects one point of that plan: a
position-like primvar is **closest** by default, not filtered. A rest
position blended across a silhouette is a point on neither surface.

## What Changes

- **`sourceType = "primvar"` is honoured on meshes.**
  - `sourceName` names the primvar, with or without the `primvars:` prefix.
  - The value is the primvar at the camera ray's first hit, interpolated as
    authored: `constant`, `uniform`, `vertex`, `varying` or `faceVarying`,
    indexed or not.
  - Where the hit surface does not author the primvar, the channel holds the
    var's clear value. The same happens where the ray escapes or hits a
    volume.
- **Subdivided and displaced meshes carry it.** A requested `vertex` /
  `varying` primvar is refined like the points. A `faceVarying` one is refined
  under the mesh's `faceVaryingLinearInterpolation`, as `st` is today, at
  uniform and adaptive levels alike. Displacement moves the surface, not the
  primvar: `Pref` stays the undisplaced rest position.
- **Curves carry it**, through the primvar decoding the `StrandTable` already
  does for materials.
- **Default accumulation follows `dataType`**:
  - points, vectors, normals and integers are *closest*;
  - colours, floats and texture coordinates are *filtered*.
  - The existing overrides (`multiSampled`, `arnold:filter`, Karma's filter,
    RenderMan's `zmin`) still apply.
- **Types are checked.** A primvar whose component count does not match the
  var's `dataType` is refused with a warning, once per var and prim; that
  prim reads the clear value. A primvar type crust cannot read at all (string,
  matrix, array-valued) is refused the same way.
- **Only what is asked for is kept.** Primvars are read and stored only when a
  var names them, so a render with no primvar var uses no extra memory or
  time, and its instruction count is unchanged. The beauty stays
  bit-identical with or without primvar vars.
- **Out of scope**, each still unavailable and documented as a known gap:
  - constant primvars inherited from ancestor prims;
  - per-instance primvars (PointInstancer `primvars:*`, primvars authored on
    instance prims);
  - primvars on implicit shapes (`Sphere`, `Cube`, …), which read the clear
    value;
  - Gaussian splats and volumes.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `aovs`: new requirements for primvar sources, their types, their
  refinement with the surface, the prims they read (no inheritance yet) and
  their zero cost when unrequested. "Accumulation modes" gains the
  `dataType`-driven default for primvar sources.
- `usd-scene-import`: "Render products and render vars" accepts
  `sourceType = "primvar"` and resolves vars before geometry loads.

## Impact

- **Code**:
  - `crust-core/src/scene/usd_import/products.rs`: the `primvar` arm and
    the keep set.
  - `usd_import/mesh.rs`: reading the kept primvars.
  - `scene/subdiv/`: N-lane refinement beside the 2-lane chart.
  - `scene/displace.rs`: carrying the primvar through the displaced
    tessellation.
  - `rt_world.rs`: a per-geometry primvar side table beside `UvMap`, and its
    live-edit rebuild.
  - `aov.rs`: a primvar source, its slot and its default accumulation.
  - the tracer's AOV first hit.
- **Interaction with `add-identity-aovs-openexrid`**: once this change is
  archived, that change's task group 1, D4 and its "Primvar sources"
  requirement are removed. Until then the two do not conflict, because this
  change's requirement replaces the one that change would add.
- **Performance**: no cost unless a var names a primvar. With one, memory
  grows by one indexed table per mesh that authors it, about 12 bytes of
  corner indices per triangle plus the values.
- **Docs**:
  - `site/content/docs/usd/aovs.md`: source types, the type rules, a
    `Pref` example and "Not supported".
  - `site/content/docs/reference/warnings.md`: any new warning code.
  - the `aovs` design record and its known gaps.
- **Dependencies**: none.
