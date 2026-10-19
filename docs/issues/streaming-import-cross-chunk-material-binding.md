# crust-render: streaming import drops material bindings whose target lies in another top-level subtree

**Target repository:** crust-render (this repo), not an upstream library
**Affected version:** `crust` 0.8.0 (`22c8ed5`)
**Severity:** high. A valid stage renders with grey surfaces, and the only signal is a misleading warning
**Status:** open. Workaround below.

---

## Summary

With streaming import on (`CRUST_STREAM_IMPORT`, the default), a mesh whose
`material:binding` targets a `Material` outside the mesh's own top-level subtree
falls back to grey. `stream_roots` opens one stage per child of the root prim,
masked to that child. The bound `Material` lives under a sibling child, so it
is not populated on the stage the mesh is imported from. `UsdMaterial::get`
returns `None`, and `resolve_material` reports `material.fallback_default`.

The stage itself is valid. OpenUSD's `UsdShade.MaterialBindingAPI.ComputeBoundMaterial()`
resolves the binding, and crust resolves it too with `CRUST_STREAM_IMPORT=0`.
This breaks the guarantee in `stream_roots`' doc comment that streaming is
"pixel-identical to the single-stage import".

Streaming only engages at `MIN_STREAM_CHUNKS` (4) top-level children or more.
So the same scene renders correctly with three children and turns grey when an
unrelated fourth sibling, such as a camera, is added.

## Reproduction

[`repro/streaming-cross-chunk-binding/scene.usda`](repro/streaming-cross-chunk-binding/scene.usda):
four children under `/World`. The sphere in `geo` binds the material in `looks`.

```usda
def Xform "World"
{
    def Xform "geo"
    {
        def Sphere "ball" ( prepend apiSchemas = ["MaterialBindingAPI"] )
        {
            rel material:binding = </World/looks/red>
        }
    }

    def Scope "looks"
    {
        def Material "red"
        {
            token outputs:surface.connect = </World/looks/red/surf.outputs:surface>
            def Shader "surf"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.8, 0.05, 0.05)
                token outputs:surface
            }
        }
    }

    def Camera "cam" { ... }
    def DistantLight "sun" { ... }
}
```

```sh
crust check -i scene.usda                          # streamed
CRUST_STREAM_IMPORT=0 crust check -i scene.usda    # single stage
```

## Actual vs expected

| | streamed (default) | `CRUST_STREAM_IMPORT=0` |
| --- | --- | --- |
| `crust check` | `skipped material.fallback_default ×1: /World/looks/red` | no warnings |
| render, sample pixel | `[8.40e-7, 8.40e-7, 8.40e-7]`: grey | `[1.36e-6, 9.58e-8, 9.58e-8]`: red |

`crust diff single_stage.exr streamed.exr` reports 91.9% of pixels differing.

Expected: identical output in both modes, with the sphere red.

The warning text adds to the problem:

```
[material.fallback_default] Material at /World/looks/red not resolvable — using default grey OpenPBR
```

It reads as if the Material were broken. It is well-formed; it just isn't on the
masked stage.

## Hypotheses ruled out

- **The material or its network is invalid.** pxr resolves the binding and the
  surface source. Crust decodes the same Material once streaming is off.
- **The shader inputs are unsupported.** Same evidence: the single-stage import
  produces the red surface.
- **Reference or payload arcs are involved.** The reproduction has none. It is a
  single layer with the binding and the Material in sibling subtrees.
- **The dependency is on authoring order.** It isn't. Removing one sibling, so
  there are fewer than four chunks, makes it pass. Moving the Material inside
  `geo`, into the same chunk, also makes it pass.

## Impact

This was found rendering a production shot (██████, `e101_s050_p040`) assembled
as one `def Xform` per Alembic-converted cache under `/World`: anim, camera,
cammap, cloth, hair, lighting. Six chunks, so streaming was on.

- Character materials defined in a `/World/materials` scope rendered grey on
  every mesh under `/World/anim_fix`.
- After moving them into `anim_fix`, the cloth-simulation caches under
  `/World/cloth` went grey for the same reason. Their bindings pointed back into
  `anim_fix`.
- Disabling streaming took the scene's parse from 3.5 s to 35 s. Most of that is
  texture loading, which the grey fallback had skipped.

The workaround is to give every chunk that binds a material its own copy of it.
An internal reference keeps a single definition:

```usda
def Xform "cloth"
{
    def Scope "materials"
    {
        def Material "███████" ( references = </World/anim_fix/materials/██████> ) {}
    }
    def Xform "████_001" ( prepend apiSchemas = ["MaterialBindingAPI"] )
    {
        rel material:binding = </World/cloth/materials/████>
    }
}
```

This works because composition copies the referenced subtree into the chunk.
It is a scene-side workaround for an importer behaviour, though. Layouts that
keep materials in a sibling `/World/Looks` scope are the norm, not an edge
case, and they all hit this.

### Possibly affected, untested

The same masking applies to any relationship or connection that crosses chunks.
Two candidates: `collection:lightLink` / `collection:shadowLink` membership on a
light in one chunk naming geometry in another, and a `NodeGraph` or interface
input shared between materials in different chunks. Lights themselves are fine:
a light in its own chunk lit geometry in other chunks correctly in the scene
above.

## Possible fixes

1. **Widen each chunk's population mask with its binding targets.** Before
   opening a chunk's masked stage, collect the `material:binding*` targets
   authored under that chunk on the index stage. Add each target path to the
   mask. A `UsdStagePopulationMask` can hold several paths, and the shader
   network below the Material comes in with it. This is the closest to
   single-stage behaviour, and it keeps the memory bound in the common case
   where materials are small.
2. **Always include material scopes.** Add every top-level subtree that holds
   only `Material` / `NodeGraph` / `Shader` prims to every chunk's mask. This is
   cheaper to implement but heuristic.
3. **Resolve on miss.** When `UsdMaterial::get` fails on a masked stage and the
   target path is outside the chunk, resolve it from a stage masked to that
   path, cached by path (`MaterialCache::key` already distinguishes stable
   paths across stages).

Whichever fix lands, a binding that fails only because of the mask should say
so: either a distinct warning code, or a message naming the chunk boundary. A
regression test in `crates/crust-core/tests` could load
`repro/streaming-cross-chunk-binding/scene.usda` in both modes and assert that
the material is not the default.
