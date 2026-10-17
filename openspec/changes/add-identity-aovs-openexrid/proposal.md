## Why

`add-usd-render-products-and-aovs` shipped products, first-hit AOVs and light
path expressions, and left its identity phase (Phase 3) unfinished: ID mattes
were re-planned from Cryptomatte to
[OpenEXRId](https://github.com/MercenariesEngineering/openexrid), which needs
deep EXR output the `exr` crate cannot write yet, and primvar AOVs were never
started. This change carries that remaining work so the finished change could
be archived. Its ID-matte half waited on a deep writer, which a fork of `exr`
now provides (design D5).

## What Changes

- **ID mattes as OpenEXRId deep EXRs**:
  - a `geom_id → prim path` table kept past import when an identity AOV is
    asked for, with the bound material and the `kind` ancestor;
  - `primId`, `instanceId` and `elementId` AOVs, and `materialId` and
    `assetId` for the mattes by material and by asset;
  - a deep scanline EXR writer (upstream in `exr`, or in-tree in safe Rust);
  - OpenEXRId output: per-pixel (id, coverage) samples weighted by the
    beauty's pixel filter, and the names the ids stand for.
- **`sourceType = "primvar"`** — not blocked: a primvar named by a RenderVar
  is kept at import and evaluated at the camera ray's first hit.
- A `deepRaster` RenderProduct naming one of those ids is the matte; the
  Cryptomatte names stay refused, with a warning pointing at the mattes.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `aovs`: "Identity and ID mattes" goes from refused to implemented, as
  requirements for identity sources, instance and element ids, OpenEXRId
  mattes and their name table; new requirements for primvar sources.
- `image-output`: a `deepRaster` product is written as an ID matte rather
  than refused, when it names its ids.

## Impact

- **Code**: `crust-core/src/scene/usd_import/` (prim-path table, primvar keep
  set), `crust-core/src/aov.rs` (identity and primvar sources), the tracer's
  first hit (prim, instance and face ids, primvar values), a deep EXR writer
  in `crust-render` or upstream in `exr`.
- **Dependencies**: `exr` patched to a fork with deep scanline reading and
  writing (safe Rust, a git source `deny.toml` allows); `flate2` and
  `base64` in `crust-render` for the name table. No C++/FFI.
- **Docs**: `site/content/docs/usd/aovs.md`, the `aovs` design record.
