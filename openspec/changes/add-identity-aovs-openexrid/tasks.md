## 1. Primvar sources (not blocked)

- [ ] 1.1 Register the primvars named by `sourceType = "primvar"` vars before
      the meshes load (D4); keep them per mesh like `st`.
- [ ] 1.2 Evaluate them at the first hit and accumulate them filtered; refuse
      a type that cannot be written as the var's `dataType`.
- [ ] 1.3 Tests: `displayColor` per mesh, the clear value where it is not
      authored, the refusal; docs.

## 2. Identity (on hold until 3.1)

- [ ] 2.1 *(on hold)* Keep a `geom_id → interned prim path` run table for
      every geometry prim when an identity AOV is requested, extending
      `LightLinks`' runs and scoped by `ImportCaches::epoch`; keep the bound
      material path and the `kind` ancestor (D1).
- [ ] 2.2 *(on hold)* Add the `primId`, `instanceId` and `elementId` AOVs
      (D2); test stable ids across frames.

## 3. OpenEXRId (on hold)

- [ ] 3.1 *(on hold)* A deep scanline EXR writer: upstream in `exr`, or
      in-tree in safe Rust. Round-trip test against a deep file OpenEXR
      itself reads.
- [ ] 3.2 *(on hold)* OpenEXRId output: per-pixel (id, coverage) samples
      weighted by the beauty's pixel filter, and the name table, in the
      layout OpenEXRId specifies (D3). Check that coverage sums to 1 on
      covered pixels.
- [ ] 3.3 *(on hold)* Docs and samples; retire the matching Known-gaps
      entries in the `aovs` design record.
