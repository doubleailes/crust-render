## Why

`add-usd-render-products-and-aovs` shipped products, first-hit AOVs and light
path expressions, and left its identity phase (Phase 3) unfinished: ID mattes
were re-planned from Cryptomatte to
[OpenEXRId](https://github.com/MercenariesEngineering/openexrid), which needs
deep EXR output the `exr` crate cannot write yet, and primvar AOVs were never
started. This change carries that remaining work so the finished change could
be archived; its ID-matte half stays on hold until a deep writer exists.

## What Changes

- **ID mattes as OpenEXRId deep EXRs** — *on hold* until crust can write
  deep EXRs:
  - a `geom_id → prim path` table kept past import when an identity AOV is
    asked for, with the bound material and the `kind` ancestor;
  - `primId`, `instanceId` and `elementId` AOVs;
  - a deep scanline EXR writer (upstream in `exr`, or in-tree in safe Rust);
  - OpenEXRId output: per-pixel (id, coverage) samples weighted by the
    beauty's pixel filter, and the names the ids stand for.
- **`sourceType = "primvar"`** — not blocked: a primvar named by a RenderVar
  is kept at import and evaluated at the camera ray's first hit.
- Until each part lands, its sources stay refused with a warning, as today.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `aovs`: "Identity and ID mattes" goes from refused to implemented (once
  unblocked); a new requirement for primvar sources.

## Impact

- **Code**: `crust-core/src/scene/usd_import/` (prim-path table, primvar keep
  set), `crust-core/src/aov.rs` (identity and primvar sources), the tracer's
  first hit (prim, instance and face ids, primvar values), a deep EXR writer
  in `crust-render` or upstream in `exr`.
- **Dependencies**: possibly a newer `exr` with deep writing; no C++/FFI (a
  deep writer behind `unsafe` would be a project decision).
- **Docs**: `site/content/docs/usd/aovs.md`, the `aovs` design record.
