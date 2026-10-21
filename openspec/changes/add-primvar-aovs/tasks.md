# Tasks

## 1. N-lane refinement, with no behaviour change

- [x] 1.1 Check whether opensubdiv-rs's `PrimvarRefiner::interpolate` /
      `limit` / `interpolate_face_varying` and the patch table's `evaluate` /
      `evaluate_face_varying` accept `[f32; 4]` (D3). Write a unit test in
      `crust-core/src/scene/subdiv/tests.rs` that refines a 4-lane vertex
      channel, whose first two lanes are the chart, and asserts they are
      bit-identical to today's `[f32; 2]` refinement. If the crate refuses
      the width, record the fallback (width-specialised calls) in the design.
- [x] 1.2 Generalise `UvChannel` / `RefinedUvs` / `SubdivRequest` (uniform)
      and `TessellatedMesh` / `Emitted` / the cage-face stitch (adaptive) to
      a list of N-lane channels, with the chart as one of them. FVar
      channels are evaluated by index, not channel 0. Verify:
      - `cargo test -p crust-core` (the subdiv and adaptive tests);
      - `scripts/check_images.sh check` against goldens recorded from `main`
        on the subdivided and adaptive samples, all bit-identical;
      - a callgrind count on a subdivided sample shows import within noise
        of `main`.

## 2. Primvar vars resolve, with an empty film

- [x] 2.1 Accept `sourceType = "primvar"` in `products.rs`:
      - strip a `primvars:` prefix;
      - refuse an empty `sourceName` and a four-component `dataType`, each
        with a warning code;
      - build the deduplicated `PrimvarKeep` (D1) and return it beside the
        request.

      Verify with unit tests for each scenario of the `usd-scene-import`
      delta ("Primvar var accepted", "Primvar var without a name refused")
      and for "Four components refused".
- [x] 2.2 Add `AovSource::Primvar` with `SlotKey.primvar: u16`. Add the
      `dataType` role, which sets the default accumulation and the
      `ChannelKind` (D5). Exclude primvar slots from `apply_exposure`. Let
      `type_fits` take the arity from the `dataType`. Verify with aov.rs
      unit tests:
      - `point3f` / `normal3f` / `int` default to closest;
      - `color3f` / `float` / `texCoord2f` default to filtered;
      - `multiSampled` still overrides;
      - an `int` var is written as UINT.
- [x] 2.3 Thread the keep set into `ImportCaches` and `MeshNeeds`. Feed the
      first hit an all-clear primvar scratch, so a primvar var renders its
      clear value everywhere. Verify:
      - a `samples/` scene with a `Pref` var renders without a warning and
        with a closest-mode, all-zero `Pref` channel;
      - the beauty is bit-identical to the run without the var;
      - the zero-AOV callgrind count is unchanged.
- [x] 2.4 Register the new warning codes in
      `site/content/docs/reference/warnings.md` (the warnings test keeps
      them in step). Verify `cargo test -p crust-core` passes.

## 3. Mesh primvar tables (no subdivision)

- [x] 3.1 Write an N-lane `PrimvarSource` reader in `usd_import/mesh.rs`:
      - all five interpolations, `:indices`, and the length checks;
      - `int` read as a scalar;
      - refusal of strings, tokens and matrices, and of a component-count
        mismatch, with one warning per var and prim.

      Verify with unit tests on inline USDA for each interpolation and each
      refusal ("Mismatched component count", "Integer IDs").
- [x] 3.2 Build `PrimvarTable` in `triangulate` (constant / per-triangle /
      corners, D2). Add the kept channels to `MeshKey`. Attach the table
      with `World::set_primvars`, leaving `plain` alone, on every
      `set_uv_map` site: the bake paths, instance placement and
      `attach_proto_parts`. Verify with tests:
      - two prims sharing points with different `rest` get different tables;
      - an instanced prototype's instances share one table;
      - `plain` is still set on a mesh with only a primvar table.
- [x] 3.3 Add `prim_id` to the AOV first hit and to `first_wall`'s tuple.
      Write `World::primvars_at`: barycentrics from `p` and the triangle's
      vertices through `VertexSource`, and the `swapped` corner order
      undone (D4). Fill the scratch only when the layout has primvar slots.
      Verify with the `aovs` delta scenarios as integration tests in
      `crust-core/tests/`, at `-s 16`:
      - "Rest position" (translated at frame 5);
      - "Prefix is optional" (bitwise);
      - "Clear value where not authored";
      - "Face-varying UV set";
      - "Per-face colour";
      - "A rest position is never blended across an edge";
      - "A colour primvar is filtered".

      The beauty stays bit-identical in each.
- [x] 3.4 Verify that tiles and scanlines agree bitwise for a primvar
      product (the existing "Tiles and scanlines agree for every AOV" test
      gains a primvar var).
- [x] 3.5 Update `site/content/docs/usd/aovs.md`:
      - the `primvar` row of "Source types";
      - a "Primvars" section with the `Pref` example, the interpolation and
        type rules, and the `dataType` accumulation defaults;
      - the int-as-UINT note;
      - "Not supported": drop `sourceType = "primvar"`, and add inherited,
        per-instance and implicit-shape primvars and four components.

      Verify `zola build` in `site/` (0.21) passes its link check.

## 4. Subdivided and displaced meshes

- [x] 4.1 Pass the kept `vertex` / `varying` channels and `faceVarying`
      channels through `SubdivRequest`, and map the results back in
      `MeshSource::subdivided` / `tessellated`. A `uniform` channel maps
      each refined triangle through its base face. Verify with "Rest
      position on a subdivided mesh" (`rest` vs `P` within float rounding)
      at uniform level 3 and with adaptive tessellation.
- [x] 4.2 Verify "Displacement leaves the rest position": a displaced mesh's
      `rest` matches its undisplaced rest position and differs from `P`.
- [x] 4.3 Verify a `loop` mesh and a `bilinear` (displacement-only) mesh
      carry a face-varying primvar like the chart, with a test per scheme.

## 5. Curves, inheritance, live edits, stats

- [x] 5.1 Add the keep set to `StrandRequest`, so a `StrandTable` is built
      for a curves prim that authors a kept primvar even when its material
      reads none. Read the channel from the hit's `CurveHit` in the AOV
      path. Verify "Curves primvar" as an integration test, and that a groom
      with no primvar var still builds no table.
- [x] 5.2 Walk the ancestors for kept names missing on a geometry prim
      (memoised per `(name, ancestor)`), and check `PointInstancer` and
      instance prims for `primvars:<name>`. Raise `PrimvarNotInherited`
      once per var (D6). Verify "Ancestor primvar is reported, not
      inherited", plus an instancer case.
- [x] 5.3 Record the keep set in `StageFacts`. Make the settings refresh in
      `usd_import/live.rs` refuse a changed keep set, so it falls back to a
      re-import (D7). Verify with a live test:
      - retargeting `products` to a product with a primvar var re-imports;
      - the result is bitwise equal to a fresh import of the same layer.
- [x] 5.4 Add `primvar_tables_bytes` to `SceneCounters` and print it with
      `--stats` (D8). Verify "No primvar var" (0 bytes, beauty bit-identical)
      and "Primvar tables are reported". Measure an AOV-only `StrandTable`
      on a groom sample with `scripts/bench_ab.sh` and record min and mean
      in the `aovs` design record.

## 6. Integration and records

- [x] 6.1 Add a sample `samples/` scene: a subdivided mesh with `rest`, a
      second UV set and `displayColor`, plus a product with `Pref`, `uv2`
      and `displayColor` vars. Verify it renders warning-free with
      `crust check --json -`. Verify each channel in numbers with
      `crust diff` against a recorded golden, not by eye.
- [x] 6.2 Run CI locally and verify all four pass:
      - `cargo fmt --all -- --check`;
      - `cargo clippy --workspace --all-targets -- -D warnings`;
      - `cargo test --workspace --no-fail-fast`;
      - the pinned-nightly clippy.
- [x] 6.3 Update the `aovs` design record (`openspec/specs/aovs/design.md`):
      - the primvar decisions (keep set, table outside `plain`, recomputed
        barycentrics, `dataType` roles);
      - known gaps: inheritance, per-instance primvars, four components,
        implicit shapes, `Val` packing.

      Remove task group 1, D4 and the "Primvar sources" requirement delta
      from `openspec/changes/add-identity-aovs-openexrid`. Verify
      `openspec validate --all` passes.

## Workflow follow-up

- File the latent `UvSource` bug (a `uniform` / `constant` `primvars:st`
  is indexed per point) as its own issue.
- Archive the change after review. Close issue #337 with the PR.
