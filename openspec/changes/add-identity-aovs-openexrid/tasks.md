## 1. Primvar sources (not blocked)

- [x] 1.1 Register the primvars named by `sourceType = "primvar"` vars before
      the meshes load (D4); keep them per mesh like `st`.
- [x] 1.2 Evaluate them at the first hit and accumulate them filtered; refuse
      a type that cannot be written as the var's `dataType`.
- [x] 1.3 Tests: `displayColor` per mesh, the clear value where it is not
      authored, the refusal; docs.

## 2. Identity

- [x] 2.1 Keep a `geom_id → interned prim path` run table for
      every geometry prim when an identity AOV is requested, extending
      `LightLinks`' runs and scoped by `ImportCaches::epoch`; keep the bound
      material path and the `kind` ancestor (D1).
- [x] 2.2 Add the `primId`, `instanceId` and `elementId` AOVs
      (D2); test stable ids across frames.

## 3. OpenEXRId

- [x] 3.1 A deep scanline EXR writer: upstream in `exr`, or
      in-tree in safe Rust. Round-trip test against a deep file OpenEXR
      itself reads. (A fork of `exr` with deep scanline support, design D5;
      `scripts/openexrid_oracle.py` reads crust's files with OpenEXR 3.5.2.)
- [x] 3.2 OpenEXRId output: per-pixel (id, coverage) samples
      weighted by the beauty's pixel filter, and the name table, in the
      layout OpenEXRId specifies (D3). Check that coverage sums to 1 on
      covered pixels.
- [x] 3.3 Docs and samples; retire the matching Known-gaps
      entries in the `aovs` design record.
