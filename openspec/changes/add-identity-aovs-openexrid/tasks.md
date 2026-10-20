## 1. Identity (on hold until 2.1)

- [ ] 1.1 *(on hold)* Keep a `geom_id → interned prim path` run table for
      every geometry prim when an identity AOV is requested, extending
      `LightLinks`' runs and scoped by `ImportCaches::epoch`; keep the bound
      material path and the `kind` ancestor (D1).
- [ ] 1.2 *(on hold)* Add the `primId`, `instanceId` and `elementId` AOVs
      (D2); test stable ids across frames.

## 2. OpenEXRId (on hold)

- [ ] 2.1 *(on hold)* A deep scanline EXR writer: upstream in `exr`, or
      in-tree in safe Rust. Round-trip test against a deep file OpenEXR
      itself reads.
- [ ] 2.2 *(on hold)* OpenEXRId output: per-pixel (id, coverage) samples
      weighted by the beauty's pixel filter, and the name table, in the
      layout OpenEXRId specifies (D3). Check that coverage sums to 1 on
      covered pixels.
- [ ] 2.3 *(on hold)* Docs and samples; retire the matching Known-gaps
      entries in the `aovs` design record.
