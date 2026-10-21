# Proposal

## Why

MaterialX `geompropvalue` read primvars only on curves (#342). On a mesh or an
implicit shape it returned its `default`, so the primvars an asset authors for
shading — per-point opacity, vertex colour, per-face ids, `displayColor` — never
reached its material. An FX mesh fading its pieces through a per-point
`primvars:Alpha` read into `opacity` rendered every piece opaque.

## What Changes

- **A mesh's material reads the mesh's primvars by name.** `constant`, `uniform`,
  `vertex`, `varying` and `faceVarying`, indexed or not, in the types curves read
  (`float`, `float2`, `float3`, `color3f` converted from its `colorSpace`, `int`).
  A subdivided mesh refines them with the surface, as it refines the chart.
- **Implicit shapes read constant primvars**, the analytic sphere included; any
  other interpolation is refused with a warning.
- **One table for both readers.** The material's primvars ride the mesh's existing
  `PrimvarTable` (the primvar AOVs'), decoded and refined by the same code, in a
  second list (`shading`). A geometry whose table holds one leaves
  `World::intersect`'s plain path and its hits carry a `MeshHit`, in `HitPart`
  beside the Ptex face and the strand; a mesh whose material reads no primvar is
  untouched.
- **`ShadeCtx::primvars`** replaces `CurveCtx::primvars` in crust-mtlx: any hit
  can supply them.
- New warning `mesh.unsupported_primvar`.

## Impact

- `crust-mtlx` (`ShadeCtx`, `CurveCtx`, `Op::Primvar`), `crust-core`
  (`PrimvarTable`, `MeshHit`, `HitPart::Mesh`, `World::intersect`,
  `Material::primvars`, `materialx::run`, the mesh, implicit-shape, sphere and
  prototype imports, `Signature`).
- `openspec/specs/materials` (spec and design record), `usd-scene-import` and
  `aovs` design records; the user documentation's geometry, materials,
  limitations and warnings pages.
