## Phase 1: products, vars, geometric AOVs

## 1. Product and var resolution (`usd_import/settings.rs`)

- [x] 1.1 Resolve settings → `products` → `RenderProduct` → `orderedVars` →
      `RenderVar` with the typed `openusd-schemas` views, at `eval_time()`. A
      product overlays only the base attributes it authors (D2), keeps
      `orderedVars` order, and uses a var targeted twice in one product
      once. Accept `sourceType` and `driver:parameters:aov:name` authored as
      string or token.
- [x] 1.2 Collect `driver:parameters:*` (product) and
      `driver:parameters:aov:*` (var), plus the renderer filter attributes
      named in D7, by prefix-filtering `authored_property_names()`.
- [x] 1.3 Build an `AovRequest` on `RenderSettings` (built: on `Scene`, see
      design.md § Phase 1 as built) that lists, per product:
      path, layers, sources, type, accumulation mode and clear value. Take the
      render's camera and resolution from the first product. Refuse
      `deepRaster`, a mismatched camera or resolution, `intrinsic`, unknown
      sources and type mismatches, each with one `WARN` (D17).
- [x] 1.4 Tests:
      - inheritance and override;
      - time-sampled `productName`;
      - the Houdini-authored var from `arnold-usd` `test_0228` (D4, D7);
      - each refusal;
      - no products → empty request;
      - the resolver agrees with `openusd_schemas::render::compute_render_spec`
        on a time-invariant fixture.

## 2. Film and first-hit AOVs (`tracer/`)

- [x] 2.1 Make `trace_path` generic over `const AOV: bool`. With
      `AOV = true`, fill a `FirstHit` (position, distance, depth, shading and
      geometric normals, UV, hit/escape) in `PathScratch` at vertex 0. Expose
      the shading normal from `ShadingPoint` to `crate` only.
- [x] 2.2 Add per-unit SoA AOV planes beside `PixelState`:
      - filtered accumulation uses the beauty's `wx·wy`;
      - closest accumulation keeps (depth, value) for in-box samples, with a
        nearest-to-centre fallback (D7);
      - also track alpha, `sampleCount`, and `variance` (from `var_map`).
- [x] 2.3 Copy the planes in the serial gather; return them through a new
      `render_with_aovs` entry point. Blend guided passes with the beauty's
      weights (D9).
- [x] 2.4 Tests:
      - beauty bit-identical with and without AOVs;
      - every channel bit-identical across tiles and scanlines;
      - depth never blended at an edge;
      - normal on a sphere;
      - alpha is 0 on a dome-only pixel;
      - `sampleCount` equals spp with adaptive off.
- [x] 2.5 Gate the zero-AOV path:
      - callgrind instruction count on cornellbox at `-s 2` is unchanged (within
        callgrind's run-to-run noise of zero);
      - `scripts/check_images.sh check` passes against goldens recorded
        before the change.

## 3. EXR writer (`crust-render/src/main.rs`)

- [x] 3.1 Write each product as a single-part, scanline, ZIP16 EXR through
      `AnyChannels::sort`, following `crust-assets/src/tiled/exr_write.rs`.
      Name channels as D15 says, with HALF/FLOAT/UINT precision, the
      `software` and `colorInteropID` headers, and copied
      `driver:parameters` text attributes. Create parent directories.
- [x] 3.2 Keep the no-products path on `write_rgb_file`, byte-identical.
- [x] 3.3 Make the PNG from the first product's beauty.
- [x] 3.4 Teach `examples/exr_diff` to diff every named channel. It reads
      only the first layer today.
- [x] 3.5 Tests: channel names and order for the D15 examples; half on
      request; UINT `-1`; the no-products EXR is byte-identical to a pre-change
      reference.

## 4. CLI and docs

- [x] 4.1 Make `-o` an `Option<String>` with D16 semantics. Update
      `cli_defaults_when_nothing_is_given` and `cli_parses_its_flags`.
- [x] 4.2 Add one `INFO` line listing the products written.
- [x] 4.3 Add `samples/aovs.usda`: a Solaris-style `/Render` with two products
      covering every Phase 1 source and one deliberately unknown var.
- [x] 4.4 Run `scripts/material_fidelity` on one case and confirm its output
      paths are unchanged (Migration Plan).
- [x] 4.5 Docs:
      - new `openspec/specs/image-output/design.md`;
      - new `site/content/docs/usd/aovs.md` (the D5 table, spaces, modes,
        the `depth` divergence);
      - update `usd/render-settings.md` and `reference/command-line.md`;
      - add the AOV pairs to `docs/architecture.md` § Invariants;
      - add a CLAUDE.md documentation-table row for `aovs`;
      - `zola build` passes.

## Phase 2: lobe-aware AOVs and LPE

## 5. Lobe taxonomy (`material/`)

- [ ] 5.1 Give every OpenPBR `Lobe` and every `closure::Lobe` variant an
      event (`R`/`T` × `D`/`G`/`S`/`s`) and a label (D10). Write an exhaustive
      `match` test so that a new lobe cannot be added unlabelled. A lobe at
      zero roughness and delta samples are `S`.
- [ ] 5.2 Add per-lobe evaluation that returns `eval_all`'s summands (and
      the NEE equivalents) without changing `eval_all`'s result. Pin that
      `Σ f_j == eval_all` bitwise where the sum order matches, and within
      1 ulp elsewhere.
- [ ] 5.3 Add `Material::albedo()` (D12), with a default of 1 and a `DEBUG`
      count of materials that use it. Add the first-non-delta-hit `albedo`
      AOV.

## 6. LPE engine (`crust-core/src/lpe/`)

- [ ] 6.1 Write the parser for the OSL grammar subset (D10). Refused tokens
      give a `WARN` with the column.
- [ ] 6.2 Build Thompson NFA → one combined DFA for all LPE vars, with
      per-state accepting bitmasks and a `u16` state. Fold labels to symbol
      indices at import. Cap at 64 LPE vars.
- [ ] 6.3 Tests on the parser and DFA, against the OSL wiki's examples and
      the canonical list in `design.md`.

## 7. Routing (`tracer/path.rs`)

- [ ] 7.1 During the forward walk, record per-vertex DFA state(s), per-lobe
      NEE shares, per-lobe bounce factors, and the `L`/`O` class of each
      emission. Do this only in the `AOV` instantiation.
- [ ] 7.2 Run the masked backward gather per (AOV, reachable state) (D11),
      with the "lobes agree" fast path. Reuse the beauty's vertex-0 clamp
      factor (D9).
- [ ] 7.3 Read `crust:light:lpeTag` on every light kind, with the fallback
      attributes from D13 once they are verified.
- [ ] 7.4 Tests:
      - `C.*[LO]` bit-identical to the beauty;
      - the partition from the spec sums to the beauty;
      - with `--indirect-clamp`, the partition still sums;
      - the lobe split converges as 1/√N to per-lobe references (the bias
        test that would fail under label-by-picked-lobe);
      - light groups;
      - NEE-only versus BSDF-only (`--strategy light|bsdf`) agree per AOV in
        expectation.
- [ ] 7.5 Add `samples/aovs_lpe.usda` with the compositing set (direct and
      indirect diffuse and glossy, transmission, emission, volume) and two
      light groups.
- [ ] 7.6 Record the cost per added LPE var (callgrind, cornellbox) and
      `bench_ab.sh` on the scene set in the `aovs` design record.

## Phase 3: identity

## 8. Prim identity and Cryptomatte

- [ ] 8.1 Keep a `geom_id → interned prim path` run table for every
      geometry prim when an identity AOV is requested, extending
      `LightLinks`' runs and scoped by `ImportCaches::epoch`. Also keep the
      bound material path and the `kind` ancestor.
- [ ] 8.2 Add the `primId`, `instanceId` and `elementId` (authored face
      index) AOVs.
- [ ] 8.3 Write MurmurHash3_x86_32 in safe Rust and test it against the
      Cryptomatte vector (`"torus"`).
- [ ] 8.4 Add Cryptomatte accumulation (fixed per-pixel rank arrays plus
      spill), layers, and header manifest. Check that coverage sums to 1 on
      covered pixels.
- [ ] 8.5 Add `sourceType = "primvar"`: register the requested primvars
      before mesh load, keep them per mesh, and evaluate them at the first
      hit.
- [ ] 8.6 Docs and samples for Phase 3. Retire the matching Known-gaps
      entries.
